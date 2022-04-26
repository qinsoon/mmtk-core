extern crate proc_macro;
extern crate syn;
extern crate proc_macro_error;
extern crate quote;

use proc_macro::TokenStream;
use proc_macro_error::proc_macro_error;
use syn::{parse_macro_input};
use proc_macro_error::{abort, abort_call_site};
use quote::quote;
use syn::{spanned::Spanned, Attribute, DeriveInput, Field, FieldsNamed, TypeGenerics};
use quote::ToTokens;
use syn::__private::TokenStream2;
use syn::token::Token;

mod util;

#[proc_macro_error]
#[proc_macro_derive(PlanTraceObject, attributes(trace, main_policy, copy, fallback_trace))]
pub fn derive_plan_trace_object(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let output = if let syn::Data::Struct(syn::DataStruct {
        fields: syn::Fields::Named(ref fields),
        ..
    }) = input.data {
        let spaces = util::get_fields_with_attribute(fields, "trace");
        let main_policy = util::get_unique_field_with_attribute(fields, "main_policy");
        let fallback = util::get_unique_field_with_attribute(fields, "fallback_trace");

        let trace_object_function = generate_trace_object(&spaces, &fallback, &ty_generics);
        let create_scan_work_function = generate_create_scan_work(&main_policy, &ty_generics);
        let may_move_objects_function = generate_may_move_objects(&main_policy, &ty_generics);
        quote!{
            impl #impl_generics crate::plan::transitive_closure::PlanTraceObject #ty_generics for #ident #ty_generics #where_clause {
                #[inline(always)]
                #trace_object_function

                #[inline(always)]
                #create_scan_work_function

                #[inline(always)]
                #may_move_objects_function
            }
        }
    } else {
        abort_call_site!("`#[derive(PlanTraceObject)]` only supports structs with named fields.")
    };

    // Debug the output
    // println!("{}", output.to_token_stream());

    output.into()
}

fn generate_trace_object<'a>(
    space_fields: &[&'a Field],
    parent_field: &Option<&'a Field>,
    ty_generics: &TypeGenerics,
) -> TokenStream2 {
    let space_field_handler = space_fields.iter().map(|f| {
        let f_ident = f.ident.as_ref().unwrap();
        let ref f_ty = f.ty;

        // Figure out copy
        let trace_attr = util::get_field_attribute(f, "trace").unwrap();
        // parse tts
        let copy = if !trace_attr.tokens.is_empty() {
            // let copy = trace_attr.parse_meta().unwrap();
            // println!("copy {:?}", copy.name());
            use syn::AttributeArgs;
            use syn::Token;
            use syn::NestedMeta;
            use syn::punctuated::Punctuated;

            let attr_tokens: TokenStream = trace_attr.tokens.clone().into();
            let args = trace_attr.parse_args_with(Punctuated::<NestedMeta, Token![,]>::parse_terminated).unwrap();
            if let Some(NestedMeta::Meta(syn::Meta::Path(p))) = args.first() {
                quote!{ Some(#p) }
            } else {
                quote!{ None }
            }
        } else {
            quote!{ None }
        };

        quote! {
            if self.#f_ident.in_space(__mmtk_objref) {
                return <#f_ty as PolicyTraceObject #ty_generics>::trace_object::<T, KIND>(&self.#f_ident, __mmtk_trace, __mmtk_objref, #copy, __mmtk_worker);
            }
        }
    });

    let parent_field_delegator = if let Some(f) = parent_field {
        let f_ident = f.ident.as_ref().unwrap();
        let ref f_ty = f.ty;
        quote! {
            <#f_ty as PlanTraceObject #ty_generics>::trace_object::<T, KIND>(&self.#f_ident, __mmtk_trace, __mmtk_objref, __mmtk_worker)
        }
    } else {
        quote! {
            panic!("No more spaces to try")
        }
    };

    quote! {
        fn trace_object<T: crate::plan::TransitiveClosure, const KIND: crate::policy::gc_work::TraceKind>(&self, __mmtk_trace: &mut T, __mmtk_objref: crate::util::ObjectReference, __mmtk_worker: &mut crate::scheduler::GCWorker<VM>) -> crate::util::ObjectReference {
            use crate::policy::space::Space;
            use crate::plan::transitive_closure::PolicyTraceObject;
            use crate::plan::transitive_closure::PlanTraceObject;
            #(#space_field_handler)*
            #parent_field_delegator
        }
    }
}

fn generate_create_scan_work<'a>(
    scan_work: &Option<&'a Field>,
    ty_generics: &TypeGenerics,
) -> TokenStream2 {
    if let Some(f) = scan_work {
        let f_ident = f.ident.as_ref().unwrap();
        let ref f_ty = f.ty;

        quote! {
            fn create_scan_work<E: crate::scheduler::gc_work::ProcessEdgesWork<VM = VM>>(&'static self, nodes: Vec<crate::util::ObjectReference>) -> Box<dyn crate::scheduler::GCWork<VM>> {
                use crate::plan::transitive_closure::PolicyTraceObject;
                <#f_ty as PolicyTraceObject #ty_generics>::create_scan_work::<E>(&self.#f_ident, nodes)
            }
        }
    } else {
        quote! {
            fn create_scan_work<E: crate::scheduler::gc_work::ProcessEdgesWork<VM = VM>>(&'static self, nodes: Vec<crate::util::ObjectReference>) -> Box<dyn crate::scheduler::GCWork<VM>> {
                unreachable!()
            }
        }
    }
}

fn generate_may_move_objects<'a>(
    scan_work: &Option<&'a Field>,
    ty_generics: &TypeGenerics,
) -> TokenStream2 {
    if let Some(f) = scan_work {
        let f_ident = f.ident.as_ref().unwrap();
        let ref f_ty = f.ty;

        quote! {
            fn may_move_objects<const KIND: crate::policy::gc_work::TraceKind>() -> bool {
                use crate::plan::transitive_closure::PolicyTraceObject;
                <#f_ty as PolicyTraceObject #ty_generics>::may_move_objects::<KIND>()
            }
        }
    } else {
        quote! {
            fn may_move_objects<const KIND: crate::policy::gc_work::TraceKind>() -> bool {
                unreachable!()
            }
        }
    }
}
