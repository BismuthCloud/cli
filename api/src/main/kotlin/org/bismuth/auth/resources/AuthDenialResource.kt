package org.bismuth.auth.resources

import jakarta.ws.rs.*
import jakarta.ws.rs.core.Response

class AuthDenialResource {
    @Path("/{path: .*}")
    @GET
    fun denyGet(): Response {
        return Response.status(401).entity("Denied.").build()
    }

    @Path("/{path: .*}")
    @PUT
    fun denyPut(): Response {
        return Response.status(401).entity("Denied.").build()
    }

    @Path("/{path: .*}")
    @DELETE
    fun denyDelete(): Response {
        return Response.status(401).entity("Denied.").build()
    }

    @Path("/{path: .*}")
    @POST
    fun denyPost(): Response {
        return Response.status(401).entity("Denied.").build()
    }

    @Path("/{path: .*}")
    @OPTIONS
    fun denyOptions(): Response {
        return Response.status(401).entity("Denied.").build()
    }
}